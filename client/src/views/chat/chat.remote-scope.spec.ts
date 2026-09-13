/**
 * F10-31: when the active engine target is a paired desktop (device token,
 * WP-M1 `remote` scope) `ChatView` must not call the Local-only routes the
 * scope 403s - `GET /config` and `GET /tools/safety` - on every session open.
 * Against any other target the calls stay exactly as before.
 */

import { provideZonelessChangeDetection, signal } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';
import { ActivatedRoute, Router, convertToParamMap, provideRouter } from '@angular/router';
import { of } from 'rxjs';

import { EngineClient } from '../../core/engine-client.service';
import { EngineTargetStore } from '../../core/engine-target.store';
import { Message, SessionMeta } from '../../core/engine.dtos';
import { EventsStore } from '../../core/events.store';
import { ChatView } from './chat';

function sessionMeta(): SessionMeta {
  return {
    id: 's1',
    directory: '/p',
    agent: 'code',
    model: 'zai/glm-4.5',
    created_at: 0,
    updated_at: 0,
    usage: { input_tokens: 0, output_tokens: 0 },
    running: false,
  } as SessionMeta;
}

const MESSAGES: Message[] = [
  { id: 'm1', role: 'user', parts: [{ type: 'text', text: 'hi' }] } as Message,
];

async function settle(fixture: ComponentFixture<ChatView>): Promise<void> {
  for (let i = 0; i < 8; i++) {
    await Promise.resolve();
    await fixture.whenStable();
  }
  fixture.detectChanges();
}

interface EngineMock {
  connected: ReturnType<typeof signal<boolean>>;
  connect: jasmine.Spy;
  sessionMeta: jasmine.Spy;
  messages: jasmine.Spy;
  listAgents: jasmine.Spy;
  getConfig: jasmine.Spy;
  getToolSafety: jasmine.Spy;
  pendingPermissions: jasmine.Spy;
  prompt: jasmine.Spy;
  abort: jasmine.Spy;
}

describe('ChatView remote scope gating (F10-31)', () => {
  let fixture: ComponentFixture<ChatView>;
  let engine: EngineMock;

  async function mount(kind: 'desktop' | 'remote-url'): Promise<void> {
    engine = {
      connected: signal(true),
      connect: jasmine.createSpy('connect').and.resolveTo({}),
      sessionMeta: jasmine.createSpy('sessionMeta').and.resolveTo(sessionMeta()),
      messages: jasmine.createSpy('messages').and.resolveTo(MESSAGES),
      listAgents: jasmine.createSpy('listAgents').and.resolveTo([{ name: 'code' }]),
      getConfig: jasmine.createSpy('getConfig').and.resolveTo({
        config: { thinking: 'medium', provider: 'zai', model: 'glm-4.5' },
        providers: [{ name: 'zai', has_key: true, models: ['glm-4.5'] }],
        mcp: [],
        skills: [],
      }),
      getToolSafety: jasmine
        .createSpy('getToolSafety')
        .and.resolveTo({ tools: [], layers: { global: {}, project: {} } }),
      pendingPermissions: jasmine.createSpy('pendingPermissions').and.resolveTo([]),
      prompt: jasmine.createSpy('prompt').and.resolveTo({}),
      abort: jasmine.createSpy('abort').and.resolveTo({}),
    };
    const events = {
      start: () => undefined,
      onEvent: () => () => undefined,
      reconnectVersion: signal(0),
    };
    localStorage.clear();
    TestBed.configureTestingModule({
      imports: [ChatView],
      providers: [
        provideZonelessChangeDetection(),
        provideRouter([]),
        { provide: EngineClient, useValue: engine },
        { provide: EventsStore, useValue: events },
        {
          provide: ActivatedRoute,
          useValue: {
            paramMap: of(convertToParamMap({ sessionID: 's1' })),
            snapshot: {
              firstChild: null,
              data: {},
              paramMap: convertToParamMap({ sessionID: 's1' }),
            },
          },
        },
      ],
    });
    spyOn(TestBed.inject(Router), 'navigate').and.resolveTo(true);
    const targets = TestBed.inject(EngineTargetStore);
    await targets.ready;
    targets.upsert({
      id: `${kind}:t1`,
      kind,
      label: kind === 'desktop' ? 'Desktop · pc' : 'http://pc:8790',
      baseUrl: 'http://pc:8790',
      token: 'tok',
    });
    targets.setActive(`${kind}:t1`);
    expect(targets.remoteScope()).toBe(kind === 'desktop');

    fixture = TestBed.createComponent(ChatView);
    fixture.detectChanges();
    await fixture.componentInstance.ngOnInit();
    await settle(fixture);
  }

  afterEach(() => {
    fixture?.destroy();
    localStorage.clear();
  });

  it('skips GET /config and GET /tools/safety against a paired desktop', async () => {
    await mount('desktop');
    const view = fixture.componentInstance;

    expect(engine.sessionMeta).toHaveBeenCalledWith('s1');
    expect(engine.listAgents).toHaveBeenCalledWith('/p');
    expect(engine.getConfig).not.toHaveBeenCalled();
    expect(engine.getToolSafety).not.toHaveBeenCalled();
    // The switchers fall back to the session's own model/agent.
    expect(view.agents().length).toBe(1);
    expect(view.availableModels()).toEqual([]);
    expect(view.configDefaultModel()).toBeNull();
    expect(view.loading()).toBeFalse();
    expect(view.error()).toBeNull();
  });

  it('keeps calling them against an engine address (full local scope)', async () => {
    await mount('remote-url');
    const view = fixture.componentInstance;

    expect(engine.getConfig).toHaveBeenCalledWith('/p');
    expect(engine.getToolSafety).toHaveBeenCalledWith('/p');
    expect(view.thinking()).toBe('medium');
    expect(view.availableModels()).toEqual(['zai/glm-4.5']);
  });
});
