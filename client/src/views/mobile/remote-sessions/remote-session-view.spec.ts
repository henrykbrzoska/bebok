/**
 * WP-M6 (F10-24/F10-25/F10-27): the thin wrapper around `ChatView` - hosts
 * the chat, shows the abort bar while a turn runs (POST /session/{id}/abort),
 * the permission sheet, and the offline banner + queue.
 */

import { Component, input, provideZonelessChangeDetection, signal } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';

import { EngineClient } from '../../../core/engine-client.service';
import { ENGINE_API } from '../../../core/engine-api';
import { EngineTargetStore } from '../../../core/engine-target.store';
import { EventsStore } from '../../../core/events.store';
import { MemoryCacheStore, OFFLINE_CACHE_STORE, OfflineCache, OfflineQueue } from '../../../core/remote/offline-cache';
import { PermissionPopup } from '../../../ui/permission-popup/permission-popup';
import { ChatView } from '../../chat/chat';
import { ChatSessionStore } from '../../chat/chat-session.store';
import { RemoteSessionView, cachedText } from './remote-session-view';

@Component({ selector: 'app-chat', template: '<div data-testid="stub-chat">chat</div>' })
class StubChat {}

@Component({ selector: 'app-permission-popup', template: '<div data-testid="stub-perm"></div>' })
class StubPermission {
  readonly mode = input<'inline' | 'sheet'>('inline');
  readonly activeSessionID = input<string>();
  readonly directory = input<string>();
}

describe('RemoteSessionView (WP-M6)', () => {
  let fixture: ComponentFixture<RemoteSessionView>;
  let engine: { abort: jasmine.Spy; connection: ReturnType<typeof signal<null>>; unauthorized: ReturnType<typeof signal<boolean>> };
  let events: { state: ReturnType<typeof signal<string>>; onEvent: jasmine.Spy; reconnectVersion: ReturnType<typeof signal<number>> };
  let chat: ChatSessionStore;
  let targets: EngineTargetStore;

  beforeEach(() => {
    localStorage.clear();
    engine = {
      connection: signal(null),
      unauthorized: signal(false),
      abort: jasmine.createSpy('abort').and.resolveTo({ ok: true }),
    };
    events = {
      state: signal('live'),
      onEvent: jasmine.createSpy('onEvent').and.returnValue(() => undefined),
      reconnectVersion: signal(0),
    };
    TestBed.configureTestingModule({
      imports: [RemoteSessionView],
      providers: [
        provideZonelessChangeDetection(),
        { provide: EngineClient, useValue: engine },
        { provide: ENGINE_API, useValue: engine },
        { provide: EventsStore, useValue: events },
        { provide: OFFLINE_CACHE_STORE, useValue: new MemoryCacheStore() },
      ],
    });
    TestBed.overrideComponent(RemoteSessionView, {
      remove: { imports: [ChatView, PermissionPopup] },
      add: { imports: [StubChat, StubPermission] },
    });
    chat = TestBed.inject(ChatSessionStore);
    targets = TestBed.inject(EngineTargetStore);
    targets.upsert({ id: 'desktop:1', kind: 'desktop', label: 'rafal-pc', baseUrl: 'http://1', token: 't' });
    targets.setActive('desktop:1');
    fixture = TestBed.createComponent(RemoteSessionView);
    fixture.componentRef.setInput('sessionID', 's1');
    fixture.detectChanges();
  });

  afterEach(() => {
    fixture.destroy();
    chat.clear();
    localStorage.clear();
  });

  const el = (): HTMLElement => fixture.nativeElement as HTMLElement;

  async function settle(): Promise<void> {
    // The cache cipher is WebCrypto (async, off the microtask queue).
    await new Promise((resolve) => setTimeout(resolve, 20));
    fixture.detectChanges();
  }

  it('hosts the ChatView and the sheet-mode permission prompt', () => {
    expect(el().querySelector('[data-testid="stub-chat"]')).not.toBeNull();
    expect(el().querySelector('app-permission-popup')).not.toBeNull();
    expect(el().querySelector('[data-testid="remote-abort-bar"]')).toBeNull();
  });

  it('shows the abort bar while running and POSTs /abort on tap', async () => {
    chat.running.set(true);
    await settle();
    const button = el().querySelector<HTMLButtonElement>('[data-testid="remote-abort"]');
    expect(button).not.toBeNull();
    button!.click();
    await settle();
    expect(engine.abort).toHaveBeenCalledWith('s1');
    chat.running.set(false);
    await settle();
    expect(el().querySelector('[data-testid="remote-abort-bar"]')).toBeNull();
  });

  it('offline: banner, queued prompt instead of a live send, abort queued too', async () => {
    events.state.set('error');
    await settle();
    expect(el().querySelector('[data-testid="remote-offline-banner"]')).not.toBeNull();
    const view = fixture.componentInstance;
    view.draft.set('hello from the lift');
    view.sendQueued();
    chat.running.set(true);
    await settle();
    el().querySelector<HTMLButtonElement>('[data-testid="remote-abort"]')!.click();
    await settle();
    const queue = TestBed.inject(OfflineQueue);
    expect(queue.pending().map((c) => c.kind)).toEqual(['prompt', 'abort']);
    expect(queue.pending()[0].payload).toEqual({ message: 'hello from the lift' });
    expect(engine.abort).not.toHaveBeenCalled();
    expect(el().querySelectorAll('[data-testid="remote-queue"] li').length).toBe(2);
  });

  it('offline with nothing live: renders the cached transcript tail', async () => {
    await TestBed.inject(OfflineCache).putTranscript('desktop:1', 's1', [
      { id: 'm1', role: 'user', parts: [{ type: 'text', text: 'cached question' }] },
      { id: 'm2', role: 'assistant', parts: [{ type: 'text', text: 'cached answer' }] },
    ]);
    // Re-create so the cache is read for the session.
    fixture.destroy();
    fixture = TestBed.createComponent(RemoteSessionView);
    fixture.componentRef.setInput('sessionID', 's1');
    fixture.detectChanges();
    events.state.set('error');
    await settle();
    await settle();
    const cached = el().querySelector('[data-testid="remote-cached-transcript"]');
    expect(cached).not.toBeNull();
    expect(cached!.textContent).toContain('cached question');
    expect(cached!.textContent).toContain('cached answer');
  });

  it('caches the live transcript tail as it changes', async () => {
    jasmine.clock().install();
    try {
      const cache = TestBed.inject(OfflineCache);
      const put = spyOn(cache, 'putTranscript').and.resolveTo();
      chat.messages.set([{ id: 'm1', role: 'user', parts: [{ type: 'text', text: 'x' }] }]);
      TestBed.tick();
      jasmine.clock().tick(1000);
      expect(put).toHaveBeenCalledWith('desktop:1', 's1', jasmine.any(Array));
    } finally {
      jasmine.clock().uninstall();
    }
  });

  it('cachedText joins text parts only', () => {
    expect(
      cachedText({
        id: 'm',
        role: 'assistant',
        parts: [
          { type: 'text', text: 'a' },
          { type: 'thinking', text: 'hidden' } as never,
          { type: 'text', text: 'b' },
        ],
      }),
    ).toBe('a\nb');
  });
});
