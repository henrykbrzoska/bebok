/**
 * F6-4: the "Compact context now" palette action. Only offered while a chat
 * session with enough messages is open; running it compacts that session and
 * navigates to the fork the engine returns.
 */

import { provideZonelessChangeDetection, signal } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';
import { Router, provideRouter } from '@angular/router';

import { EngineClient } from '../../core/engine-client.service';
import { Message, RemoteStatus, SessionMeta } from '../../core/engine.dtos';
import { RemoteDesktopStore } from '../../core/remote-desktop.store';
import { ChatSessionStore } from '../../views/chat/chat-session.store';
import { ProjectSessionsStore } from '../shell/project-sessions.store';
import { ShellStore } from '../shell/shell.store';
import { ToastStore } from '../toast/toast.store';
import { CommandPalette } from './command-palette';

function userMessage(id: string): Message {
  return { id, role: 'user', parts: [{ type: 'text', text: id }] } as Message;
}

const META: SessionMeta = {
  id: 's1',
  directory: '/p',
  agent: 'code',
  created_at: 0,
  updated_at: 0,
  usage: { input_tokens: 0, output_tokens: 0 },
  context_used: 150_000,
  context_window: 200_000,
} as SessionMeta;

describe('CommandPalette compaction action (F6-4)', () => {
  let fixture: ComponentFixture<CommandPalette>;
  let engine: { compactSession: jasmine.Spy; createSession: jasmine.Spy };
  let navigate: jasmine.Spy;
  let session: ChatSessionStore;

  beforeEach(() => {
    localStorage.clear();
    engine = {
      compactSession: jasmine
        .createSpy('compactSession')
        .and.resolveTo({ sessionID: 'fork-9', parent: ['s1', 3] }),
      createSession: jasmine.createSpy('createSession').and.resolveTo({ sessionID: 'new' }),
    };
    const shell = {
      commandPaletteOpen: signal(true),
      closeCommandPalette: jasmine.createSpy('closeCommandPalette'),
      toggleCommandPalette: jasmine.createSpy('toggleCommandPalette'),
      openProjectSwitcher: jasmine.createSpy('openProjectSwitcher'),
    };
    const project = {
      directory: signal<string | null>('/p'),
      refresh: jasmine.createSpy('refresh').and.resolveTo(undefined),
    };
    TestBed.configureTestingModule({
      imports: [CommandPalette],
      providers: [
        provideZonelessChangeDetection(),
        provideRouter([]),
        { provide: EngineClient, useValue: engine },
        { provide: ShellStore, useValue: shell },
        { provide: ProjectSessionsStore, useValue: project },
      ],
    });
    navigate = spyOn(TestBed.inject(Router), 'navigate').and.resolveTo(true);
    session = TestBed.inject(ChatSessionStore);
    fixture = TestBed.createComponent(CommandPalette);
    fixture.detectChanges();
  });

  afterEach(() => fixture.destroy());

  function ids(): string[] {
    return fixture.componentInstance.actions().map((action) => action.id);
  }

  it('hides the action without an open (long enough) session', () => {
    expect(ids()).not.toContain('compactSession');
    session.meta.set(META);
    session.messages.set([userMessage('a'), userMessage('b')]);
    expect(ids()).not.toContain('compactSession');
  });

  it('offers the action for an open session and searches it by label', () => {
    session.meta.set(META);
    session.messages.set(['a', 'b', 'c', 'd'].map(userMessage));
    expect(ids()).toContain('compactSession');
    fixture.componentInstance.query.set('compact');
    expect(ids()).toEqual(['compactSession']);
  });

  it('compacts the open session and navigates to the fork', async () => {
    session.meta.set(META);
    session.messages.set(['a', 'b', 'c', 'd'].map(userMessage));
    const action = fixture.componentInstance
      .actions()
      .find((entry) => entry.id === 'compactSession')!;
    await fixture.componentInstance.run(action);
    expect(engine.compactSession).toHaveBeenCalledWith('s1', 100_000);
    expect(navigate).toHaveBeenCalledWith(['/chat', 'fork-9']);
  });

  it('leaves a running session alone', async () => {
    session.meta.set(META);
    session.messages.set(['a', 'b', 'c', 'd'].map(userMessage));
    session.running.set(true);
    const action = fixture.componentInstance
      .actions()
      .find((entry) => entry.id === 'compactSession')!;
    await fixture.componentInstance.run(action);
    expect(engine.compactSession).not.toHaveBeenCalled();
    expect(navigate).not.toHaveBeenCalled();
  });
});

function remoteStatus(overrides: Partial<RemoteStatus> = {}): RemoteStatus {
  return {
    enabled: false,
    listening: false,
    endpoints: [],
    devicesOnline: 0,
    devices: 0,
    engineName: 'demo',
    fingerprint: 'abcd1234',
    port: 8790,
    allowLan: false,
    ...overrides,
  };
}

/** WP-M4 (F10-14): "Toggle Remote access" enables/disables without navigating. */
describe('CommandPalette toggleRemote action (F10-14)', () => {
  let fixture: ComponentFixture<CommandPalette>;
  let engine: { enableRemote: jasmine.Spy; disableRemote: jasmine.Spy };
  let remote: { ensure: jasmine.Spy; enabled: () => boolean; status: { set: jasmine.Spy } };
  let navigate: jasmine.Spy;
  let enabled: boolean;

  beforeEach(() => {
    localStorage.clear();
    enabled = false;
    engine = {
      enableRemote: jasmine.createSpy('enableRemote').and.resolveTo(remoteStatus({ enabled: true, listening: true })),
      disableRemote: jasmine.createSpy('disableRemote').and.resolveTo(remoteStatus({ enabled: false })),
    };
    remote = {
      ensure: jasmine.createSpy('ensure').and.resolveTo(undefined),
      enabled: () => enabled,
      status: { set: jasmine.createSpy('status.set') },
    };
    const shell = {
      commandPaletteOpen: signal(true),
      closeCommandPalette: jasmine.createSpy('closeCommandPalette'),
      toggleCommandPalette: jasmine.createSpy('toggleCommandPalette'),
      openProjectSwitcher: jasmine.createSpy('openProjectSwitcher'),
    };
    const project = {
      directory: signal<string | null>('/p'),
      refresh: jasmine.createSpy('refresh').and.resolveTo(undefined),
    };
    TestBed.configureTestingModule({
      imports: [CommandPalette],
      providers: [
        provideZonelessChangeDetection(),
        provideRouter([]),
        { provide: EngineClient, useValue: engine },
        { provide: RemoteDesktopStore, useValue: remote },
        { provide: ShellStore, useValue: shell },
        { provide: ProjectSessionsStore, useValue: project },
      ],
    });
    navigate = spyOn(TestBed.inject(Router), 'navigate').and.resolveTo(true);
    fixture = TestBed.createComponent(CommandPalette);
    fixture.detectChanges();
  });

  afterEach(() => fixture.destroy());

  it('is offered and searchable', () => {
    const ids = fixture.componentInstance.actions().map((a) => a.id);
    expect(ids).toContain('toggleRemote');
    fixture.componentInstance.query.set('remote');
    expect(fixture.componentInstance.actions().map((a) => a.id)).toEqual(['toggleRemote']);
  });

  it('enables Remote directly, without navigating, and toasts the result', async () => {
    const action = fixture.componentInstance.actions().find((a) => a.id === 'toggleRemote')!;
    await fixture.componentInstance.run(action);

    expect(remote.ensure).toHaveBeenCalled();
    expect(engine.enableRemote).toHaveBeenCalled();
    expect(engine.disableRemote).not.toHaveBeenCalled();
    expect(remote.status.set).toHaveBeenCalledWith(jasmine.objectContaining({ enabled: true }));
    expect(navigate).not.toHaveBeenCalled();
    const toasts = TestBed.inject(ToastStore).toasts();
    expect(toasts.length).toBe(1);
    expect(toasts[0].kind).toBe('success');
  });

  it('disables Remote when it was already on', async () => {
    enabled = true;
    const action = fixture.componentInstance.actions().find((a) => a.id === 'toggleRemote')!;
    await fixture.componentInstance.run(action);

    expect(engine.disableRemote).toHaveBeenCalled();
    expect(engine.enableRemote).not.toHaveBeenCalled();
  });
});
