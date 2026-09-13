/**
 * WP-M5 / F10-17: the native capture controls are additive on the existing
 * composer. A gallery pick goes through `ChatView.addFiles()` and is sent
 * with exactly the desktop `images[]` payload shape; dictation appends to
 * the draft and never sends; both controls hide when unsupported; the
 * foreground-service tracker is balanced around the send.
 */

import { provideZonelessChangeDetection, signal } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { of } from 'rxjs';

import { CameraService, fileFromBase64 } from '../../../core/camera.service';
import { EngineClient } from '../../../core/engine-client.service';
import { EngineWorkTracker } from '../../../core/engine-launcher';
import { SessionMeta } from '../../../core/engine.dtos';
import { EventsStore } from '../../../core/events.store';
import { SpeechService } from '../../../core/speech.service';
import { ChatView } from '../chat';
import { ComposerCapture, speechLocale } from './composer-capture';

/** 1x1 PNG. */
const PNG_BASE64 =
  'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==';

function sessionMeta(): SessionMeta {
  return {
    id: 's1',
    directory: '/p',
    agent: 'code',
    created_at: 0,
    updated_at: 0,
    usage: { input_tokens: 0, output_tokens: 0 },
    running: false,
  } as SessionMeta;
}

async function settle(fixture: ComponentFixture<unknown>): Promise<void> {
  for (let i = 0; i < 8; i++) {
    await Promise.resolve();
    await fixture.whenStable();
  }
  fixture.detectChanges();
}

describe('ComposerCapture (F10-17)', () => {
  let fixture: ComponentFixture<ChatView>;
  let engine: { prompt: jasmine.Spy; isCapacitor: boolean } & Record<string, unknown>;
  let events: { listeners: Array<(e: unknown) => void> } & Record<string, unknown>;
  let bridgeCalls: string[];

  async function mount(setup: () => void): Promise<void> {
    engine = {
      connected: signal(true),
      isCapacitor: true,
      connect: jasmine.createSpy('connect').and.resolveTo({}),
      sessionMeta: jasmine.createSpy('sessionMeta').and.resolveTo(sessionMeta()),
      messages: jasmine.createSpy('messages').and.resolveTo([]),
      listAgents: jasmine.createSpy('listAgents').and.resolveTo([]),
      getConfig: jasmine
        .createSpy('getConfig')
        .and.resolveTo({ config: { thinking: 'off' }, providers: [], mcp: [], skills: [] }),
      pendingPermissions: jasmine.createSpy('pendingPermissions').and.resolveTo([]),
      prompt: jasmine.createSpy('prompt').and.resolveTo({}),
      abort: jasmine.createSpy('abort').and.resolveTo({}),
    };
    events = {
      listeners: [],
      start: () => undefined,
      onEvent: (l: (e: unknown) => void) => {
        events.listeners.push(l);
        return () => undefined;
      },
      reconnectVersion: signal(0),
    };
    bridgeCalls = [];
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
            snapshot: { firstChild: null, data: {}, paramMap: convertToParamMap({ sessionID: 's1' }) },
          },
        },
      ],
    });
    setup();
    TestBed.inject(EngineWorkTracker).configure({
      native: true,
      bridge: {
        beginWork: async () => {
          bridgeCalls.push('begin');
        },
        endWork: async () => {
          bridgeCalls.push('end');
        },
      },
    });
    fixture = TestBed.createComponent(ChatView);
    fixture.detectChanges();
    await fixture.componentInstance.ngOnInit();
    await settle(fixture);
  }

  function el<T extends HTMLElement>(testId: string): T | null {
    return (fixture.nativeElement as HTMLElement).querySelector<T>(`[data-testid="${testId}"]`);
  }

  afterEach(() => fixture?.destroy());

  it('sends a gallery pick with the exact desktop images[] payload', async () => {
    await mount(() => {
      TestBed.inject(CameraService).configure({
        available: true,
        bridge: { getPhoto: async () => ({ base64String: PNG_BASE64, format: 'png' }) },
      });
      TestBed.inject(SpeechService).configure({ native: false });
    });

    const gallery = el<HTMLButtonElement>('capture-gallery');
    expect(gallery).not.toBeNull();
    gallery!.click();
    const view = fixture.componentInstance;
    // The staged file is read through FileReader (a macrotask): poll for it.
    for (let i = 0; i < 50 && view.attachments().length === 0; i++) {
      await new Promise((resolve) => setTimeout(resolve, 20));
    }
    await settle(fixture);
    expect(view.attachments().length).toBe(1);
    const staged = view.attachments()[0];
    expect(staged.media_type).toBe('image/png');
    expect(staged.base64).toBe(PNG_BASE64);

    view.draft.set('what is this?');
    await view.sendPrompt();
    await settle(fixture);

    expect(engine.prompt).toHaveBeenCalledTimes(1);
    const [id, body] = engine.prompt.calls.mostRecent().args as [string, Record<string, unknown>];
    expect(id).toBe('s1');
    // Same shape `queuedPromptBody()` produces for a desktop file pick / paste.
    expect(Object.keys(body).sort()).toEqual(['agent', 'images', 'message']);
    expect(body['message']).toBe('what is this?');
    expect(body['agent']).toBe('code');
    expect(body['images']).toEqual([
      { media_type: 'image/png', data: PNG_BASE64, name: staged.name },
    ]);
    // Staging area cleared after send, like the desktop path.
    expect(view.attachments()).toEqual([]);

    // F10-21: beginWork on send; endWork once the engine reports idle.
    const tracker = TestBed.inject(EngineWorkTracker);
    await tracker.settled();
    expect(bridgeCalls).toEqual(['begin']);
    for (const l of events.listeners) {
      l({ type: 'session.updated', directory: '/p', sessionID: 's1', properties: { running: false } });
    }
    await tracker.settled();
    expect(bridgeCalls).toEqual(['begin', 'end']);
  });

  it('dictation fills the draft without sending', async () => {
    await mount(() => {
      TestBed.inject(CameraService).configure({ available: false });
      TestBed.inject(SpeechService).configure({
        native: true,
        bridge: {
          available: async () => ({ available: true }),
          requestPermissions: async () => ({ speechRecognition: 'granted' }),
          start: async () => ({ matches: ['fix the failing test'] }),
          stop: async () => undefined,
        },
      });
    });
    await settle(fixture);

    expect(el('capture-camera')).toBeNull();
    const mic = el<HTMLButtonElement>('capture-mic');
    expect(mic).not.toBeNull();
    fixture.componentInstance.draft.set('please');
    mic!.click();
    await settle(fixture);

    expect(fixture.componentInstance.draft()).toBe('please fix the failing test');
    expect(engine.prompt).not.toHaveBeenCalled();
  });

  it('hides camera and mic when unsupported', async () => {
    await mount(() => {
      TestBed.inject(CameraService).configure({ available: false });
      TestBed.inject(SpeechService).configure({
        native: true,
        bridge: {
          available: async () => ({ available: false }),
          requestPermissions: async () => ({ speechRecognition: 'granted' }),
          start: async () => ({ matches: [] }),
          stop: async () => undefined,
        },
      });
    });
    await settle(fixture);
    expect(el('capture-camera')).toBeNull();
    expect(el('capture-gallery')).toBeNull();
    expect(el('capture-mic')).toBeNull();
    // The desktop-only Web Speech mic is not rendered on Capacitor either.
    expect((fixture.nativeElement as HTMLElement).querySelector('.icon-action.mic')).toBeNull();
  });

  it('hides the image controls for a text-only model', async () => {
    await mount(() => {
      TestBed.inject(CameraService).configure({
        available: true,
        bridge: { getPhoto: async () => ({ base64String: PNG_BASE64, format: 'png' }) },
      });
      TestBed.inject(SpeechService).configure({ native: false });
    });
    fixture.componentInstance.selectedModel.set('deepseek/deepseek-chat');
    await settle(fixture);
    expect(el('capture-camera')).toBeNull();
    fixture.componentInstance.selectedModel.set('openai/gpt-4.1');
    await settle(fixture);
    expect(el('capture-camera')).not.toBeNull();
  });
});

describe('ComposerCapture helpers', () => {
  it('decodes a base64 photo into a typed File', () => {
    const file = fileFromBase64(PNG_BASE64, 'png', 'photo');
    expect(file.type).toBe('image/png');
    expect(file.name).toMatch(/^photo-\d+\.png$/);
    expect(file.size).toBe(70);
    const jpg = fileFromBase64(`data:image/jpeg;base64,${PNG_BASE64}`, 'jpg');
    expect(jpg.type).toBe('image/jpeg');
    expect(jpg.name).toMatch(/\.jpg$/);
  });

  it('maps the UI language to a recogniser locale', () => {
    expect(speechLocale('pl')).toBe('pl-PL');
    expect(speechLocale('en')).toBe('en-US');
    expect(speechLocale('xx')).toBe(navigator.language || 'en-US');
  });

  it('is a standalone component', () => {
    TestBed.configureTestingModule({
      imports: [ComposerCapture],
      providers: [provideZonelessChangeDetection()],
    });
    TestBed.inject(CameraService).configure({ available: false });
    TestBed.inject(SpeechService).configure({ native: false });
    const fixture = TestBed.createComponent(ComposerCapture);
    fixture.detectChanges();
    expect(fixture.nativeElement.querySelector('button')).toBeNull();
  });
});
