/**
 * F6-4: "Compact now" (toolbar) and client-side auto-compaction before the
 * next prompt. Compaction is fork-based, so both paths must navigate to the
 * *new* session id the engine returns rather than expect an in-place change.
 */

import { provideZonelessChangeDetection, signal } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';
import { ActivatedRoute, Router, convertToParamMap, provideRouter } from '@angular/router';
import { of } from 'rxjs';

import { EngineClient } from '../../core/engine-client.service';
import { Message, SessionMeta } from '../../core/engine.dtos';
import { EventsStore } from '../../core/events.store';
import { ChatView } from './chat';
import { ChatSessionStore } from './chat-session.store';

function userMessage(id: string, text: string): Message {
  return { id, role: 'user', parts: [{ type: 'text', text }] } as Message;
}

function sessionMeta(extra: Partial<SessionMeta>): SessionMeta {
  return {
    id: 's1',
    directory: '/p',
    agent: 'code',
    created_at: 0,
    updated_at: 0,
    usage: { input_tokens: 0, output_tokens: 0 },
    running: false,
    ...extra,
  } as SessionMeta;
}

const FOUR_MESSAGES: Message[] = [
  userMessage('m1', 'one'),
  userMessage('m2', 'two'),
  userMessage('m3', 'three'),
  userMessage('m4', 'four'),
];

/** Let awaited promises inside the component settle, then re-render. */
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
  pendingPermissions: jasmine.Spy;
  compactSession: jasmine.Spy;
  prompt: jasmine.Spy;
  abort: jasmine.Spy;
}

describe('ChatView compaction (F6-4)', () => {
  let fixture: ComponentFixture<ChatView>;
  let engine: EngineMock;
  let navigate: jasmine.Spy;

  async function mount(meta: SessionMeta, messages: Message[] = FOUR_MESSAGES): Promise<void> {
    engine = {
      connected: signal(true),
      connect: jasmine.createSpy('connect').and.resolveTo({}),
      sessionMeta: jasmine.createSpy('sessionMeta').and.resolveTo(meta),
      messages: jasmine.createSpy('messages').and.resolveTo(messages),
      listAgents: jasmine.createSpy('listAgents').and.resolveTo([]),
      getConfig: jasmine
        .createSpy('getConfig')
        .and.resolveTo({ config: { thinking: 'off' }, providers: [], mcp: [], skills: [] }),
      pendingPermissions: jasmine.createSpy('pendingPermissions').and.resolveTo([]),
      compactSession: jasmine
        .createSpy('compactSession')
        .and.resolveTo({ sessionID: 'fork-1', parent: ['s1', 2], before: 170_000, after: 30_000 }),
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
          // `snapshot` is walked by ShellStore.refresh() (pulled in via TextPartComponent).
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
    navigate = spyOn(TestBed.inject(Router), 'navigate').and.resolveTo(true);
    fixture = TestBed.createComponent(ChatView);
    fixture.detectChanges();
    await fixture.componentInstance.ngOnInit();
    await settle(fixture);
  }

  function compactButton(): HTMLButtonElement {
    const button = (fixture.nativeElement as HTMLElement).querySelector<HTMLButtonElement>(
      '[data-testid="compact-now"]',
    );
    expect(button).not.toBeNull();
    return button!;
  }

  afterEach(() => {
    fixture?.destroy();
  });

  it('"Compact now" calls the engine and opens the forked session', async () => {
    await mount(sessionMeta({ context_used: 42_000, context_window: 200_000 }));
    const button = compactButton();
    expect(button.disabled).toBeFalse();
    expect(fixture.componentInstance.canCompact()).toBeTrue();

    button.click();
    await settle(fixture);

    expect(engine.compactSession).toHaveBeenCalledWith('s1', 100_000);
    expect(navigate).toHaveBeenCalledWith(['/chat', 'fork-1']);
    expect(engine.prompt).not.toHaveBeenCalled();
  });

  it('renders the context meter in the toolbar once a turn has run', async () => {
    await mount(sessionMeta({ context_used: 42_000, context_window: 200_000 }));
    const meter = (fixture.nativeElement as HTMLElement).querySelector(
      '[data-testid="context-meter"]',
    );
    expect(meter?.textContent).toContain('42k / 200k · 21%');
    expect(meter?.classList.contains('warning')).toBeFalse();
  });

  it('auto-compacts before sending when the meter is at the threshold', async () => {
    // 85% of 200k: exactly the default threshold.
    await mount(sessionMeta({ context_used: 170_000, context_window: 200_000 }));
    expect(TestBed.inject(ChatSessionStore).needsAutoCompact()).toBeTrue();

    fixture.componentInstance.draft.set('next question');
    await fixture.componentInstance.sendPrompt();
    await settle(fixture);

    expect(engine.compactSession).toHaveBeenCalledWith('s1', 100_000);
    expect(navigate).toHaveBeenCalledWith(['/chat', 'fork-1']);
    // The prompt was carried over to the fork, not sent to the old session.
    expect(engine.prompt).not.toHaveBeenCalled();
  });

  it('never auto-compacts a session under the threshold', async () => {
    await mount(sessionMeta({ context_used: 168_000, context_window: 200_000 })); // 84%
    expect(TestBed.inject(ChatSessionStore).needsAutoCompact()).toBeFalse();

    fixture.componentInstance.draft.set('next question');
    await fixture.componentInstance.sendPrompt();
    await settle(fixture);

    expect(engine.compactSession).not.toHaveBeenCalled();
    expect(engine.prompt).toHaveBeenCalledTimes(1);
    expect(engine.prompt.calls.mostRecent().args[0]).toBe('s1');
  });

  it('disables "Compact now" while the transcript is too short', async () => {
    await mount(sessionMeta({ context_used: 1_000, context_window: 200_000 }), [
      userMessage('m1', 'only one'),
    ]);
    expect(fixture.componentInstance.canCompact()).toBeFalse();
    expect(compactButton().disabled).toBeTrue();
  });
});
