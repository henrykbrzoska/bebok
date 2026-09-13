/**
 * F9-12: the breadcrumb of a sub-agent chat reads "<parent> ↳ <child>" with
 * the parent clickable; an ordinary chat keeps the monospace session id.
 */

import { provideZonelessChangeDetection, signal } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';

import { EngineEvent, RemoteDevice, SessionMeta } from '../../core/engine.dtos';
import { EventsStore } from '../../core/events.store';
import { RemoteDesktopStore } from '../../core/remote-desktop.store';
import { ChatSessionStore } from '../../views/chat/chat-session.store';
import { ProjectSessionsStore } from '../shell/project-sessions.store';
import { ShellStore } from '../shell/shell.store';
import { ToastStore } from '../toast/toast.store';
import { Topbar } from './topbar';

function device(id: string, name: string): RemoteDevice {
  return {
    id,
    name,
    createdAt: 0,
    lastSeen: 0,
    lastIp: '',
    revoked: false,
    model: '',
    platform: '',
  };
}

/** A no-op `RemoteDesktopStore`/`EventsStore` double so unrelated Topbar
 *  specs never trigger a real engine fetch or SSE connection (F10-14/15). */
function remoteFakes(overrides: { enabled?: boolean; devicesOnline?: number; devices?: RemoteDevice[] } = {}) {
  const remote = {
    enabled: signal(overrides.enabled ?? false),
    devicesOnline: signal(overrides.devicesOnline ?? 0),
    devices: signal(overrides.devices ?? []),
    status: { set: () => undefined },
    ensure: jasmine.createSpy('ensure').and.resolveTo(undefined),
  };
  const events = {
    start: () => undefined,
    onEvent: jasmine.createSpy('onEvent').and.returnValue(() => undefined),
  };
  return { remote, events };
}

function session(overrides: Partial<SessionMeta>): SessionMeta {
  const now = Date.now();
  return {
    id: 'session-0000',
    directory: '/p',
    agent: 'code',
    created_at: now,
    updated_at: now,
    usage: { input_tokens: 0, output_tokens: 0 },
    ...overrides,
  } as SessionMeta;
}

describe('Topbar (F9-12 sub-agent breadcrumb)', () => {
  let fixture: ComponentFixture<Topbar>;
  let currentSessionId: ReturnType<typeof signal<string | null>>;
  let sessions: ReturnType<typeof signal<SessionMeta[]>>;
  let chat: ChatSessionStore;

  beforeEach(() => {
    currentSessionId = signal<string | null>('child-1');
    sessions = signal<SessionMeta[]>([]);
    TestBed.resetTestingModule();
    TestBed.configureTestingModule({
      imports: [Topbar],
      providers: [
        provideZonelessChangeDetection(),
        provideRouter([]),
        {
          provide: ShellStore,
          useValue: {
            isChat: signal(true),
            currentSessionId,
            activeScreen: signal('chat'),
            rightDrawerOpen: signal(true),
            toggleRightDrawer: () => undefined,
          },
        },
        { provide: ProjectSessionsStore, useValue: { sessions } },
        { provide: RemoteDesktopStore, useValue: remoteFakes().remote },
        { provide: EventsStore, useValue: remoteFakes().events },
      ],
    });
    chat = TestBed.inject(ChatSessionStore);
    fixture = TestBed.createComponent(Topbar);
    fixture.detectChanges();
  });

  it('shows the session id for an ordinary chat', () => {
    chat.meta.set(session({ id: 'child-1', title: 'Plain' }));
    fixture.detectChanges();
    const host = fixture.nativeElement as HTMLElement;
    expect(host.querySelector('[data-testid="crumb-parent"]')).toBeNull();
    expect(host.querySelector('.crumb-current.mono')?.textContent).toContain('child-1');
  });

  it('shows "<parent> ↳ <child>" with a link to the parent for a sub-agent chat', () => {
    sessions.set([session({ id: 'parent-1', title: 'Add the Orders feature' })]);
    chat.meta.set(session({ id: 'child-1', alias: 'api-orders', parent: ['parent-1', 3] }));
    fixture.detectChanges();
    const host = fixture.nativeElement as HTMLElement;
    const parent = host.querySelector('[data-testid="crumb-parent"]') as HTMLAnchorElement;
    expect(parent.textContent?.trim()).toBe('Add the Orders feature');
    expect(parent.getAttribute('href')).toBe('/chat/parent-1');
    expect(host.querySelector('[data-testid="crumb-child"]')?.textContent?.trim()).toBe(
      'api-orders',
    );
    expect(host.querySelector('.crumb-subagent svg')).toBeTruthy();
  });

  it('falls back to the short parent id while the session list has not loaded', () => {
    chat.meta.set(session({ id: 'child-1', alias: 'api-orders', parent: ['parent-1234-5678', 3] }));
    fixture.detectChanges();
    const parent = (fixture.nativeElement as HTMLElement).querySelector(
      '[data-testid="crumb-parent"]',
    );
    expect(parent?.textContent?.trim()).toBe('parent-1');
  });

  it('a fork (parent without alias) is not a sub-agent', () => {
    chat.meta.set(session({ id: 'child-1', title: 'Forked', parent: ['parent-1', 3] }));
    fixture.detectChanges();
    expect(
      (fixture.nativeElement as HTMLElement).querySelector('[data-testid="crumb-parent"]'),
    ).toBeNull();
  });
});

/** WP-M4 (F10-14): "Remote · N" pill, hidden when Remote is off. */
describe('Topbar remote pill (F10-14)', () => {
  let fixture: ComponentFixture<Topbar>;

  function build(overrides: Parameters<typeof remoteFakes>[0]): ReturnType<typeof remoteFakes> {
    const fakes = remoteFakes(overrides);
    TestBed.resetTestingModule();
    TestBed.configureTestingModule({
      imports: [Topbar],
      providers: [
        provideZonelessChangeDetection(),
        provideRouter([]),
        {
          provide: ShellStore,
          useValue: {
            isChat: signal(false),
            currentSessionId: signal(null),
            activeScreen: signal('settings'),
            rightDrawerOpen: signal(false),
            toggleRightDrawer: () => undefined,
          },
        },
        { provide: ProjectSessionsStore, useValue: { sessions: signal([]) } },
        { provide: RemoteDesktopStore, useValue: fakes.remote },
        { provide: EventsStore, useValue: fakes.events },
      ],
    });
    fixture = TestBed.createComponent(Topbar);
    fixture.detectChanges();
    return fakes;
  }

  function pill(): HTMLElement | null {
    return (fixture.nativeElement as HTMLElement).querySelector('[data-testid="topbar-remote-pill"]');
  }

  afterEach(() => fixture?.destroy());

  it('is hidden while Remote is disabled', () => {
    build({ enabled: false });
    expect(pill()).toBeNull();
  });

  it('shows the online device count and updates live', () => {
    const fakes = build({ enabled: true, devicesOnline: 2, devices: [device('a', 'Pixel'), device('b', 'iPhone')] });
    expect(pill()?.textContent?.trim()).toContain('2');
    expect(pill()?.getAttribute('title')).toContain('Pixel');
    expect(pill()?.getAttribute('title')).toContain('iPhone');

    fakes.remote.devicesOnline.set(3);
    fixture.detectChanges();
    expect(pill()?.textContent?.trim()).toContain('3');
  });

  it('ensures the shared store once on mount, so the pill is live without opening Settings', () => {
    const fakes = build({ enabled: true });
    expect(fakes.remote.ensure).toHaveBeenCalledTimes(1);
  });

  it('toasts a generic "Allowed from device" on permission.resolved while a device is online', async () => {
    let listener: ((event: EngineEvent) => void) | null = null;
    const fakes = remoteFakes({ enabled: true, devicesOnline: 1 });
    fakes.events.onEvent.and.callFake((fn: (event: EngineEvent) => void) => {
      listener = fn;
      return () => undefined;
    });
    TestBed.resetTestingModule();
    TestBed.configureTestingModule({
      imports: [Topbar],
      providers: [
        provideZonelessChangeDetection(),
        provideRouter([]),
        {
          provide: ShellStore,
          useValue: {
            isChat: signal(false),
            currentSessionId: signal(null),
            activeScreen: signal('settings'),
            rightDrawerOpen: signal(false),
            toggleRightDrawer: () => undefined,
          },
        },
        { provide: ProjectSessionsStore, useValue: { sessions: signal([]) } },
        { provide: RemoteDesktopStore, useValue: fakes.remote },
        { provide: EventsStore, useValue: fakes.events },
      ],
    });
    fixture = TestBed.createComponent(Topbar);
    fixture.detectChanges();

    listener!({
      type: 'permission.resolved',
      directory: '',
      sessionID: 's1',
      properties: { allowed: true },
    });

    const toast = TestBed.inject(ToastStore);
    expect(toast.toasts().length).toBe(1);
  });
});
