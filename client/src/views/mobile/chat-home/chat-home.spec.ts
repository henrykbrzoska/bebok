/**
 * WP-M5 / F10-18: chat home - recent sessions newest-first from the fake
 * `EngineApi` session lists (merged across the quick directory and the
 * projects), the empty state with one primary action, the quick session,
 * the disconnected state, and the quick-directory helpers.
 */

import { provideZonelessChangeDetection, signal } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';
import { Router, provideRouter } from '@angular/router';

import { ENGINE_API } from '../../../core/engine-api';
import { EngineTargetStore } from '../../../core/engine-target.store';
import { SessionMeta } from '../../../core/engine.dtos';
import { EventsStore } from '../../../core/events.store';
import { ProjectsStore } from '../../../core/projects.store';
import { ChatHomeView, directoryName, relativeTime } from './chat-home';
import {
  quickDirFromHome,
  readCachedQuickDir,
  resolveQuickDirectory,
  writeCachedQuickDir,
} from './quick-directory';

function session(id: string, directory: string, updated: number, extra: Partial<SessionMeta> = {}): SessionMeta {
  return {
    id,
    directory,
    agent: 'code',
    created_at: updated - 1000,
    updated_at: updated,
    usage: { input_tokens: 0, output_tokens: 0 },
    ...extra,
  } as SessionMeta;
}

const HOME = '/data/user/0/dev.bebok.mobile/files';
const QUICK = `${HOME}/.bebok/quick`;

describe('ChatHomeView (F10-18)', () => {
  let fixture: ComponentFixture<ChatHomeView>;
  interface EngineFake {
    connected: ReturnType<typeof signal<boolean>>;
    browseDirectory: jasmine.Spy;
    readLastDirectory: jasmine.Spy;
    saveDirectory: jasmine.Spy;
    listSessions: jasmine.Spy;
    listAgents: jasmine.Spy;
    createSession: jasmine.Spy;
  }
  let engine: EngineFake;
  let sessionsByDir: Record<string, SessionMeta[]>;
  let listeners: Array<(e: unknown) => void>;
  let navigate: jasmine.Spy;

  function el<T extends HTMLElement>(testId: string): T | null {
    return (fixture.nativeElement as HTMLElement).querySelector<T>(`[data-testid="${testId}"]`);
  }

  async function settle(): Promise<void> {
    for (let i = 0; i < 8; i++) {
      await Promise.resolve();
      await fixture.whenStable();
    }
    fixture.detectChanges();
  }

  function mount(connected = true, projects: string[] = []): void {
    listeners = [];
    engine = {
      connected: signal(connected),
      browseDirectory: jasmine.createSpy('browseDirectory').and.resolveTo({
        path: null,
        entries: [
          { name: '/', path: '/', hidden: false, readable: true },
          { name: 'Home', path: HOME, hidden: false, readable: true },
        ],
      }),
      readLastDirectory: jasmine.createSpy('readLastDirectory').and.returnValue(null),
      saveDirectory: jasmine.createSpy('saveDirectory'),
      listSessions: jasmine
        .createSpy('listSessions')
        .and.callFake(async (dir: string) => sessionsByDir[dir] ?? []),
      listAgents: jasmine.createSpy('listAgents').and.resolveTo([
        { name: 'code', builtin: true, description: 'Writes code' },
        { name: 'ask', builtin: true },
      ]),
      createSession: jasmine.createSpy('createSession').and.resolveTo({ sessionID: 'new-1' }),
    };
    TestBed.configureTestingModule({
      imports: [ChatHomeView],
      providers: [
        provideZonelessChangeDetection(),
        provideRouter([]),
        { provide: ENGINE_API, useValue: engine },
        {
          provide: EventsStore,
          useValue: {
            state: signal('live'),
            reconnectVersion: signal(0),
            onEvent: (l: (e: unknown) => void) => {
              listeners.push(l);
              return () => undefined;
            },
          },
        },
        {
          provide: ProjectsStore,
          useValue: {
            projects: signal(
              projects.map((path, i) => ({ id: `p${i}`, name: directoryName(path), path })),
            ),
          },
        },
      ],
    });
    TestBed.inject(EngineTargetStore).upsert({
      id: 'embedded',
      kind: 'embedded',
      label: 'This device',
      baseUrl: 'http://127.0.0.1:1',
      token: null,
      ephemeral: true,
    });
    TestBed.inject(EngineTargetStore).setActive('embedded');
    navigate = spyOn(TestBed.inject(Router), 'navigate').and.resolveTo(true);
    fixture = TestBed.createComponent(ChatHomeView);
    fixture.detectChanges();
  }

  beforeEach(() => {
    localStorage.clear();
    sessionsByDir = {};
  });

  afterEach(() => {
    fixture?.destroy();
    localStorage.clear();
  });

  it('lists sessions newest-first across the quick directory and projects', async () => {
    sessionsByDir = {
      [QUICK]: [session('q-old', QUICK, 1000), session('q-new', QUICK, 5000, { title: 'Quick one' })],
      '/projects/app': [session('p-mid', '/projects/app', 3000, { alias: 'api' })],
    };
    mount(true, ['/projects/app']);
    await settle();

    expect(engine.browseDirectory).toHaveBeenCalledTimes(1);
    expect(readCachedQuickDir('embedded')).toBe(QUICK);
    expect(engine.listSessions.calls.allArgs().map((a) => a[0])).toEqual([QUICK, '/projects/app']);
    const rows = [...(fixture.nativeElement as HTMLElement).querySelectorAll('[data-testid^="session-"]')];
    expect(rows.map((r) => r.getAttribute('data-testid'))).toEqual([
      'session-q-new',
      'session-p-mid',
      'session-q-old',
    ]);
    expect(rows[0].textContent).toContain('Quick one');
    expect(rows[0].textContent).toContain('quick');
    expect(rows[1].textContent).toContain('api');
    expect(rows[1].textContent).toContain('app');

    // Tapping a session opens the existing chat route.
    (rows[1] as HTMLButtonElement).click();
    expect(navigate).toHaveBeenCalledWith(['/m/chat', 'p-mid']);
  });

  it('shows the empty state with the quick session as the one primary action', async () => {
    mount();
    await settle();
    expect(el('chat-home-empty')).not.toBeNull();
    expect(el('chat-home-sessions')).toBeNull();
    const quick = el<HTMLButtonElement>('chat-home-quick')!;
    expect(quick.disabled).toBeFalse();
    const agent = el<HTMLSelectElement>('chat-home-agent')!;
    expect([...agent.options].map((o) => o.value)).toEqual(['code', 'ask']);

    agent.value = 'ask';
    agent.dispatchEvent(new Event('change'));
    await settle();
    quick.click();
    await settle();

    expect(engine.createSession).toHaveBeenCalledWith(QUICK, 'ask');
    expect(engine.saveDirectory).toHaveBeenCalledWith(QUICK);
    expect(navigate).toHaveBeenCalledWith(['/m/chat', 'new-1']);
  });

  it('re-lists when the engine reports a session change', async () => {
    mount();
    await settle();
    expect(engine.listSessions).toHaveBeenCalledTimes(1);
    sessionsByDir = { [QUICK]: [session('q-1', QUICK, 1)] };
    for (const l of listeners) {
      l({ type: 'session.created', directory: QUICK, sessionID: 'q-1' });
    }
    await settle();
    expect(engine.listSessions).toHaveBeenCalledTimes(2);
    expect(el('session-q-1')).not.toBeNull();
  });

  it('offers "Set up" when no engine is connected', async () => {
    mount(false);
    await settle();
    let setup = 0;
    fixture.componentInstance.setup.subscribe(() => setup++);
    expect(el('chat-home-disconnected')).not.toBeNull();
    expect(engine.listSessions).not.toHaveBeenCalled();
    el<HTMLButtonElement>('chat-home-setup')!.click();
    expect(setup).toBe(1);
  });
});

describe('quick directory helpers', () => {
  beforeEach(() => localStorage.clear());
  afterEach(() => localStorage.clear());

  it('derives <home>/.bebok/quick and caches it per target', async () => {
    expect(quickDirFromHome('C:/Users/rafal/')).toBe('C:/Users/rafal/.bebok/quick');
    expect(quickDirFromHome(HOME)).toBe(QUICK);
    const engine = {
      browseDirectory: jasmine.createSpy('browseDirectory').and.resolveTo({
        path: null,
        entries: [{ name: 'Home', path: HOME, hidden: false, readable: true }],
      }),
      readLastDirectory: () => '/last',
    };
    expect(await resolveQuickDirectory(engine, 'embedded')).toBe(QUICK);
    expect(await resolveQuickDirectory(engine, 'embedded')).toBe(QUICK);
    expect(engine.browseDirectory).toHaveBeenCalledTimes(1);
    writeCachedQuickDir('desktop:x', '/elsewhere');
    expect(await resolveQuickDirectory(engine, 'desktop:x')).toBe('/elsewhere');
  });

  it('falls back to the last directory, then the first project', async () => {
    const failing = {
      browseDirectory: async () => {
        throw new Error('403 remote_scope');
      },
      readLastDirectory: () => null,
    };
    expect(await resolveQuickDirectory(failing, 'desktop:y', [])).toBeNull();
    expect(
      await resolveQuickDirectory(failing, 'desktop:y', [
        { id: 'p', name: 'p', path: '/p', added_at: 0, last_opened_at: null, pinned: false },
      ]),
    ).toBe('/p');
    expect(await resolveQuickDirectory({ ...failing, readLastDirectory: () => '/last' }, 'd')).toBe('/last');
  });

  it('formats rows', () => {
    expect(directoryName('/projects/app/')).toBe('app');
    expect(directoryName('C:\\projects\\bebok')).toBe('bebok');
    const now = 10 * 60 * 60 * 1000;
    expect(relativeTime(now, now)).toBe('now');
    expect(relativeTime(now - 3 * 60_000, now)).toBe('3m');
    expect(relativeTime(now - 2 * 3_600_000, now)).toBe('2h');
    expect(relativeTime(now - 5 * 86_400_000 - 1, now + 5 * 86_400_000)).toBe('10d');
  });
});
