import { provideZonelessChangeDetection, signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { Router } from '@angular/router';

import { ENGINE_API } from './engine-api';
import { EngineClient } from './engine-client.service';
import { EngineTargetStore } from './engine-target.store';
import { EventsStore } from './events.store';
import { ProjectSessionsStore } from '../ui/shell/project-sessions.store';
import { WorkspaceModeStore } from './workspace-mode.store';

const PROJECT = '/work/alpha';
const CHAT_DIR = '/data/bebok/chat';

describe('WorkspaceModeStore (1.8 chat / code modes)', () => {
  let store: WorkspaceModeStore;
  let project: ProjectSessionsStore;
  let engine: jasmine.SpyObj<EngineClient>;
  let router: jasmine.SpyObj<Router>;

  beforeEach(() => {
    localStorage.clear();
    engine = jasmine.createSpyObj<EngineClient>('EngineClient', [
      'readLastDirectory',
      'connected',
      'saveDirectory',
      'listSessions',
      'listAgents',
      'chatWorkspace',
    ]);
    engine.readLastDirectory.and.returnValue(PROJECT);
    engine.connected.and.returnValue(true);
    engine.listSessions.and.resolveTo([]);
    engine.listAgents.and.resolveTo([]);
    engine.chatWorkspace.and.resolveTo({ directory: CHAT_DIR });
    router = jasmine.createSpyObj<Router>('Router', ['navigateByUrl']);
    router.navigateByUrl.and.resolveTo(true);

    TestBed.configureTestingModule({
      providers: [
        provideZonelessChangeDetection(),
        { provide: EngineClient, useValue: engine },
        { provide: ENGINE_API, useValue: engine },
        { provide: Router, useValue: router },
        { provide: EngineTargetStore, useValue: { activeId: signal('local') } },
        {
          provide: EventsStore,
          useValue: { onEvent: jasmine.createSpy('onEvent'), reconnectVersion: signal(0) },
        },
      ],
    });
    project = TestBed.inject(ProjectSessionsStore);
    store = TestBed.inject(WorkspaceModeStore);
  });

  it('defaults to code mode', () => {
    expect(store.mode()).toBe('code');
    expect(store.isChat()).toBeFalse();
  });

  it('chat mode selects the engine scratch directory without persisting it', async () => {
    await store.setMode('chat');
    expect(engine.chatWorkspace).toHaveBeenCalled();
    expect(project.directory()).toBe(CHAT_DIR);
    expect(engine.saveDirectory).not.toHaveBeenCalledWith(CHAT_DIR);
    expect(store.isChatDirectory(CHAT_DIR)).toBeTrue();
    expect(localStorage.getItem('bebok.workspaceMode')).toBe('chat');
    expect(router.navigateByUrl).toHaveBeenCalledWith('/');
  });

  it('code mode restores the remembered project', async () => {
    await store.setMode('chat');
    await store.setMode('code');
    expect(project.directory()).toBe(PROJECT);
    expect(engine.saveDirectory).toHaveBeenCalledWith(PROJECT);
    expect(store.isChat()).toBeFalse();
  });

  it('survives an engine without the route (keeps code mode usable)', async () => {
    engine.chatWorkspace.and.rejectWith(new Error('404'));
    await store.setMode('chat');
    expect(store.chatDirectory()).toBeNull();
    expect(store.error()).toContain('404');
  });
});
