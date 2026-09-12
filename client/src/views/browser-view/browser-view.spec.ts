/**
 * WP-BROWSER2 (F7-6): the browser viewer window mirrors the agent's browser
 * from `browser.frame` events / `GET /browser/frame` and drives it through
 * `POST /browser/{action}`.
 */

import { provideZonelessChangeDetection } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap } from '@angular/router';

import { EngineClient } from '../../core/engine-client.service';
import { BrowserFrame, BrowserState, EngineEvent } from '../../core/engine.dtos';
import { EventsStore } from '../../core/events.store';
import { BrowserView, clickToPage, normalizeTypedUrl } from './browser-view';

const SID = 's1';

function state(over: Partial<BrowserState> = {}): BrowserState {
  return {
    sessionID: SID,
    directory: '/p',
    display: 'viewer',
    open: true,
    headed: false,
    url: 'https://example.com/',
    title: 'Example Domain',
    running: false,
    streaming: false,
    ...over,
  };
}

function frame(seq: number, over: Partial<BrowserFrame> = {}): BrowserFrame {
  return {
    sessionID: SID,
    directory: '/p',
    url: 'https://example.com/',
    title: 'Example Domain',
    media_type: 'image/jpeg',
    data: '/9j/',
    width: 1280,
    height: 800,
    seq,
    headed: false,
    ...over,
  };
}

function frameEvent(seq: number, over: Partial<BrowserFrame> = {}): EngineEvent {
  return {
    type: 'browser.frame',
    directory: '/p',
    sessionID: SID,
    properties: frame(seq, over) as unknown as Record<string, unknown>,
  };
}

describe('BrowserView helpers (F7-6)', () => {
  it('maps clicks on a letterboxed image back to page pixels', () => {
    // 1280x800 page drawn into a 640x480 box: scale 0.5, drawn 640x400,
    // centred vertically (40px bars top and bottom).
    const box = { width: 640, height: 480 };
    const natural = { width: 1280, height: 800 };
    expect(clickToPage(box, natural, { x: 0, y: 40 })).toEqual({ x: 0, y: 0 });
    expect(clickToPage(box, natural, { x: 320, y: 240 })).toEqual({ x: 640, y: 400 });
    expect(clickToPage(box, natural, { x: 640, y: 440 })).toEqual({ x: 1280, y: 800 });
    // Letterbox margins are not clicks on the page.
    expect(clickToPage(box, natural, { x: 100, y: 10 })).toBeNull();
    expect(clickToPage(box, natural, { x: 100, y: 470 })).toBeNull();
    // Degenerate sizes never divide by zero.
    expect(clickToPage({ width: 0, height: 0 }, natural, { x: 1, y: 1 })).toBeNull();
    expect(clickToPage(box, { width: 0, height: 0 }, { x: 1, y: 1 })).toBeNull();
  });

  it('adds https:// to bare hosts and keeps explicit schemes', () => {
    expect(normalizeTypedUrl('example.com')).toBe('https://example.com');
    expect(normalizeTypedUrl('  example.com/path?q=1 ')).toBe('https://example.com/path?q=1');
    expect(normalizeTypedUrl('http://localhost:3000')).toBe('http://localhost:3000');
    expect(normalizeTypedUrl('file:///tmp/x.html')).toBe('file:///tmp/x.html');
    expect(normalizeTypedUrl('')).toBe('');
    expect(normalizeTypedUrl('   ')).toBe('');
  });
});

describe('BrowserView (F7-6)', () => {
  let fixture: ComponentFixture<BrowserView>;
  let component: BrowserView;
  let engine: {
    connected: jasmine.Spy;
    connect: jasmine.Spy;
    browserState: jasmine.Spy;
    browserFrame: jasmine.Spy;
    browserAction: jasmine.Spy;
    isTauri: jasmine.Spy;
  };
  let events: { onEvent: jasmine.Spy; start: jasmine.Spy };
  let listener: ((ev: EngineEvent) => void) | null;
  let unsubscribed: boolean;

  function setup(session: string | null = SID): void {
    listener = null;
    unsubscribed = false;
    engine = {
      connected: jasmine.createSpy('connected').and.returnValue(true),
      connect: jasmine.createSpy('connect').and.returnValue(Promise.resolve()),
      browserState: jasmine.createSpy('browserState').and.returnValue(Promise.resolve(state())),
      browserFrame: jasmine.createSpy('browserFrame').and.returnValue(Promise.resolve(frame(1))),
      browserAction: jasmine
        .createSpy('browserAction')
        .and.returnValue(Promise.resolve({ sessionID: SID, ok: true, url: 'https://x/' })),
      isTauri: jasmine.createSpy('isTauri').and.returnValue(false),
    };
    events = {
      onEvent: jasmine.createSpy('onEvent').and.callFake((fn: (ev: EngineEvent) => void) => {
        listener = fn;
        return () => {
          unsubscribed = true;
        };
      }),
      start: jasmine.createSpy('start'),
    };
    const route = {
      snapshot: { queryParamMap: convertToParamMap(session ? { session } : {}) },
    };
    TestBed.resetTestingModule();
    TestBed.configureTestingModule({
      imports: [BrowserView],
      providers: [
        provideZonelessChangeDetection(),
        { provide: EngineClient, useValue: engine },
        { provide: EventsStore, useValue: events },
        { provide: ActivatedRoute, useValue: route },
      ],
    });
    fixture = TestBed.createComponent(BrowserView);
    component = fixture.componentInstance;
  }

  async function init(): Promise<void> {
    fixture.detectChanges();
    await component.ngOnInit();
    await fixture.whenStable();
    fixture.detectChanges();
  }

  function el(): HTMLElement {
    return fixture.nativeElement as HTMLElement;
  }

  afterEach(() => fixture?.destroy());

  it('reads the session from the query string, loads state + first frame and starts SSE', async () => {
    setup();
    await init();
    expect(component.sessionId()).toBe(SID);
    expect(engine.browserState).toHaveBeenCalledWith(SID);
    expect(engine.browserFrame).toHaveBeenCalledWith(SID);
    expect(events.start).toHaveBeenCalled();
    const img = el().querySelector<HTMLImageElement>('[data-testid="bv-frame"]');
    expect(img).not.toBeNull();
    expect(img!.src).toContain('data:image/jpeg;base64,/9j/');
    expect(el().querySelector<HTMLInputElement>('[data-testid="bv-url"]')!.value).toBe(
      'https://example.com/',
    );
    expect(el().querySelector('[data-testid="bv-open-external"]')).not.toBeNull();
  });

  it('shows an error without a session id and never touches the engine', async () => {
    setup(null);
    await init();
    expect(el().textContent).toContain('No session given');
    expect(engine.browserState).not.toHaveBeenCalled();
    expect(events.start).not.toHaveBeenCalled();
  });

  it('connects first when the engine is not connected yet', async () => {
    setup();
    engine.connected.and.returnValue(false);
    await init();
    expect(engine.connect).toHaveBeenCalled();
    expect(engine.browserState).toHaveBeenCalled();
  });

  it('treats a 404 frame as "no browser open" rather than an error', async () => {
    setup();
    engine.browserState.and.returnValue(Promise.resolve(state({ open: false, headed: null })));
    engine.browserFrame.and.returnValue(
      Promise.reject(new Error('engine GET /session/s1/browser/frame -> 404: no page')),
    );
    await init();
    expect(el().querySelector('[data-testid="bv-empty"]')).not.toBeNull();
    expect(el().querySelector('[data-testid="bv-error"]')).toBeNull();
    // Navigation is still possible (it opens the browser).
    expect(el().querySelector<HTMLButtonElement>('[data-testid="bv-go"]')!.disabled).toBeFalse();
    // Page-dependent controls are not.
    expect(el().querySelector<HTMLButtonElement>('[data-testid="bv-back"]')!.disabled).toBeTrue();
  });

  it('applies browser.frame events for its own session, newest seq wins', async () => {
    setup();
    await init();
    expect(listener).not.toBeNull();
    listener!(frameEvent(5, { data: 'NEW=', url: 'https://example.com/2' }));
    await fixture.whenStable();
    fixture.detectChanges();
    expect(component.frame()!.data).toBe('NEW=');
    expect(component.currentUrl()).toBe('https://example.com/2');
    expect(el().querySelector<HTMLInputElement>('[data-testid="bv-url"]')!.value).toBe(
      'https://example.com/2',
    );
    // An older frame arriving late is dropped.
    listener!(frameEvent(3, { data: 'OLD=' }));
    expect(component.frame()!.data).toBe('NEW=');
    // Other sessions are ignored.
    listener!({ ...frameEvent(9, { data: 'OTHER=' }), sessionID: 'other' });
    expect(component.frame()!.data).toBe('NEW=');
  });

  it('keeps a URL the user is editing when frames arrive', async () => {
    setup();
    await init();
    component.onUrlInput('https://typed.example');
    listener!(frameEvent(2, { url: 'https://example.com/3' }));
    expect(component.urlDraft()).toBe('https://typed.example');
  });

  it('clears the frame on browser.closed', async () => {
    setup();
    await init();
    listener!({ type: 'browser.closed', directory: '/p', sessionID: SID });
    await fixture.whenStable();
    fixture.detectChanges();
    expect(component.frame()).toBeNull();
    expect(component.open()).toBeFalse();
    expect(el().querySelector('[data-testid="bv-empty"]')).not.toBeNull();
  });

  it('navigate normalises the URL and posts a navigate action', async () => {
    setup();
    await init();
    component.onUrlInput('example.org');
    await component.navigate();
    expect(engine.browserAction).toHaveBeenCalledWith(SID, 'navigate', {
      url: 'https://example.org',
    });
    // A frame is fetched after every action (in case nobody streamed).
    expect(engine.browserFrame.calls.count()).toBeGreaterThan(1);
  });

  it('back/forward/reload post their actions', async () => {
    setup();
    await init();
    await component.back();
    await component.forward();
    await component.reload();
    const actions = engine.browserAction.calls.allArgs().map((a) => a[1]);
    expect(actions).toEqual(['back', 'forward', 'reload']);
  });

  it('type sends the text (with submit flag) and clears the draft on success', async () => {
    setup();
    await init();
    component.typeDraft.set('hello');
    component.typeSubmit.set(true);
    await component.typeText();
    expect(engine.browserAction).toHaveBeenCalledWith(SID, 'type', {
      text: 'hello',
      submit: true,
    });
    expect(component.typeDraft()).toBe('');
  });

  it('shows the tool message when the engine reports ok:false and keeps the draft', async () => {
    setup();
    await init();
    engine.browserAction.and.returnValue(
      Promise.resolve({ sessionID: SID, ok: false, text: 'error: no element matches selector' }),
    );
    component.typeDraft.set('hello');
    await component.typeText();
    await fixture.whenStable();
    fixture.detectChanges();
    expect(component.typeDraft()).toBe('hello');
    expect(el().querySelector('[data-testid="bv-notice"]')!.textContent).toContain(
      'no element matches selector',
    );
  });

  it('surfaces a 403 (permission deny rule) as a denied error', async () => {
    setup();
    await init();
    engine.browserAction.and.returnValue(
      Promise.reject(
        new Error(
          "engine POST /session/s1/browser/navigate -> 403: browser_open is denied by permission rule 'browser_*'",
        ),
      ),
    );
    component.onUrlInput('https://blocked.example');
    await component.navigate();
    await fixture.whenStable();
    fixture.detectChanges();
    const error = el().querySelector('[data-testid="bv-error"]')!.textContent ?? '';
    expect(error).toContain('Denied by a permission rule');
    expect(error).toContain('browser_*');
  });

  it('does not overlap actions while one is busy', async () => {
    setup();
    await init();
    let resolve: (v: unknown) => void = () => undefined;
    engine.browserAction.and.returnValue(new Promise((r) => (resolve = r)));
    const first = component.reload();
    expect(component.busy()).toBeTrue();
    await component.back();
    expect(engine.browserAction.calls.count()).toBe(1);
    resolve({ sessionID: SID, ok: true });
    await first;
    expect(component.busy()).toBeFalse();
  });

  it('translates a click on the frame image into a coordinate click', async () => {
    setup();
    await init();
    const img = el().querySelector<HTMLImageElement>('[data-testid="bv-frame"]')!;
    spyOn(img, 'getBoundingClientRect').and.returnValue({
      left: 10,
      top: 20,
      width: 640,
      height: 400,
      right: 650,
      bottom: 420,
      x: 10,
      y: 20,
      toJSON: () => ({}),
    } as DOMRect);
    spyOnProperty(img, 'naturalWidth').and.returnValue(1280);
    spyOnProperty(img, 'naturalHeight').and.returnValue(800);
    component.onFrameClick({ clientX: 10 + 320, clientY: 20 + 200 } as MouseEvent);
    await fixture.whenStable();
    expect(engine.browserAction).toHaveBeenCalledWith(SID, 'click', { x: 640, y: 400 });
  });

  it('close posts close and re-reads the state', async () => {
    setup();
    await init();
    engine.browserState.and.returnValue(Promise.resolve(state({ open: false, headed: null })));
    await component.closeBrowser();
    expect(engine.browserAction).toHaveBeenCalledWith(SID, 'close', {});
    expect(component.frame()).toBeNull();
    expect(component.open()).toBeFalse();
  });

  it('shows the headed badge when the browser has a visible window', async () => {
    setup();
    engine.browserFrame.and.returnValue(Promise.resolve(frame(1, { headed: true })));
    await init();
    expect(el().querySelector('[data-testid="bv-headed"]')).not.toBeNull();
  });

  it('unsubscribes from SSE on destroy', async () => {
    setup();
    await init();
    fixture.destroy();
    expect(unsubscribed).toBeTrue();
  });
});
