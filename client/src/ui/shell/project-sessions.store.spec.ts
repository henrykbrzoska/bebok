import { provideZonelessChangeDetection, signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';

import { AgentInfo, SessionMeta } from '../../core/engine.dtos';
import { EngineClient } from '../../core/engine-client.service';
import { EventsStore } from '../../core/events.store';
import { ProjectSessionsStore } from './project-sessions.store';

const ALPHA = 'C:\\work\\alpha';
const BETA = 'C:\\work\\beta';

const alphaSession = session('alpha-session', ALPHA);
const betaSession = session('beta-session', BETA);
const agents: AgentInfo[] = [{ name: 'code', description: 'Code', builtin: true }];

describe('ProjectSessionsStore', () => {
  let store: ProjectSessionsStore;
  let engine: jasmine.SpyObj<EngineClient>;

  beforeEach(() => {
    engine = jasmine.createSpyObj<EngineClient>('EngineClient', [
      'readLastDirectory',
      'connected',
      'saveDirectory',
      'listSessions',
      'listAgents',
    ]);
    engine.readLastDirectory.and.returnValue(null);
    engine.connected.and.returnValue(true);

    TestBed.configureTestingModule({
      providers: [
        provideZonelessChangeDetection(),
        { provide: EngineClient, useValue: engine },
        {
          provide: EventsStore,
          useValue: {
            onEvent: jasmine.createSpy('onEvent'),
            reconnectVersion: signal(0),
          },
        },
      ],
    });
    store = TestBed.inject(ProjectSessionsStore);
  });

  it('clears the previous project sessions before loading the selected project', async () => {
    engine.listSessions.and.callFake((directory: string) =>
      Promise.resolve(directory === ALPHA ? [alphaSession] : [betaSession]),
    );
    engine.listAgents.and.resolveTo(agents);
    await store.select(ALPHA);

    let resolveBeta!: (value: SessionMeta[]) => void;
    engine.listSessions.and.callFake((directory: string) =>
      directory === BETA
        ? new Promise<SessionMeta[]>((resolve) => (resolveBeta = resolve))
        : Promise.resolve([alphaSession]),
    );

    const selectingBeta = store.select(BETA);
    expect(store.sessions()).toEqual([]);

    resolveBeta([betaSession]);
    await selectingBeta;
    expect(store.sessions()).toEqual([betaSession]);
  });

  it('ignores a late response from the project that was previously selected', async () => {
    engine.listSessions.and.callFake((directory: string) =>
      Promise.resolve(directory === ALPHA ? [alphaSession] : [betaSession]),
    );
    engine.listAgents.and.resolveTo(agents);
    await store.select(ALPHA);

    let resolveOldRequest!: (value: SessionMeta[]) => void;
    let alphaCalls = 0;
    engine.listSessions.and.callFake((directory: string) => {
      if (directory === ALPHA && ++alphaCalls === 1) {
        return new Promise<SessionMeta[]>((resolve) => (resolveOldRequest = resolve));
      }
      return Promise.resolve([betaSession]);
    });

    const staleRefresh = store.refresh();
    const selectingBeta = store.select(BETA);
    await selectingBeta;
    resolveOldRequest([alphaSession]);
    await staleRefresh;

    expect(store.directory()).toBe(BETA);
    expect(store.sessions()).toEqual([betaSession]);
  });
});

function session(id: string, directory: string): SessionMeta {
  return {
    id,
    directory,
    agent: 'code',
    created_at: 1,
    updated_at: 1,
    usage: { input_tokens: 0, output_tokens: 0 },
  };
}
